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
this gate red without any commit here. That is what happened between 2026-08-19
and 2026-09-04, when the same installer version and toolchain built suite commit
`b4d3101` instead of `2bc0c3f` and the gate failed on every branch. Two issues
track closing the gap: [whitaker#402][whitaker-suite-pin] asks the installer
for a ref or suite-version input, and
[shared-actions#454][shared-actions-suite-pin] asks `install-whitaker` to
expose and pass it through.

[whitaker-suite-pin]: https://github.com/leynos/whitaker/issues/402
[shared-actions-suite-pin]: https://github.com/leynos/shared-actions/issues/454

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
`pg_embedded_setup_unpriv` and `pg_worker`. Each archive is published alongside
a `sha256sum`-compatible `.tgz.sha256` sidecar, so consumers can pin and verify
a download without a second network round trip.

The `create-release` and `publish-release` jobs deliberately run without a
checkout: they only call the GitHub API. They therefore set
`GH_REPO: ${{ github.repository }}`, because `gh` otherwise infers the
repository from a git remote and fails with `not a git repository`. For the
same reason `create-release` checks tag existence through
`gh api repos/<owner>/<repo>/git/ref/tags/<tag>` rather than
`gh release create --verify-tag`, which needs a local clone.
`audit-draft-assets` requests `contents: write` even though it only reads,
because draft releases are invisible to read-scoped tokens.

`build-assets` checks out the release tag, so the packaging script always comes
from the tagged tree while the workflow itself comes from the default branch.
The job therefore backfills any missing `.sha256` sidecar before uploading,
which lets tags cut before sidecar support was added still publish a complete
asset set.

The release workflow invokes `scripts/release_archive.py` through `uv run`, so
Python 3.13 and the script dependencies are provisioned explicitly on every
runner. The script builds the selected production binaries, applies the Windows
`.exe` suffix when staging the archive, rejects path-like `target` and
`--binary` values before joining filesystem paths, and writes the shared
`cargo-binstall` `.tgz` layout plus its checksum sidecar. `Cargo.toml` exposes
matching `[package.metadata.binstall]` entries so
`cargo binstall pg-embed-setup-unpriv` can install those published assets on
the supported host triples.

Pull-request CI also performs a local `cargo-binstall` install-and-run check on
Linux, macOS, and Windows using cargo-binstall 1.19.1, verifying the generated
sidecar on each runner. The release workflow audits published asset URLs with
the same pinned cargo-binstall bootstrap before the draft release is published,
and verifies every downloaded sidecar first.

Run `make test-scripts` to exercise the Python release tooling. It covers the
archive packager and the workflow contract tests in
`scripts/tests/test_release_workflow_contract.py`, which assert that every
`gh`-invoking job has a checkout or `GH_REPO`, that release-writing jobs request
`contents: write`, and that the staged archive name and members render the
`[package.metadata.binstall]` templates exactly.

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

## Test timeouts: four tiers, outermost last

Four independent timers can end a test run, and the canonical statement of how
they must be ordered lives in the `generate-coverage` README in
[`leynos/shared-actions`][shared-actions-coverage]. All four are set here.

| Tier                     | What it bounds                     | Where it is set                               | Current value                                   |
| ------------------------ | ---------------------------------- | --------------------------------------------- | ----------------------------------------------- |
| Per-test `slow-timeout`  | one test                           | `.config/nextest.toml`                        | 180 s default; 30 s and 360 s for two overrides |
| nextest `global-timeout` | the whole test run                 | `.config/nextest.toml`                        | 600 s (10 m)                                    |
| Cargo watchdog           | one `cargo` invocation, wall clock | `RUN_RUST_CARGO_WAIT_TIMEOUT` at job level    | 1,800 s (30 m)                                  |
| Job `timeout-minutes`    | the whole job                      | job level in `ci.yml` and `coverage-main.yml` | 65 m                                            |

*Table: the timers that can end a run, innermost first.*

### The outermost tier was missing

Neither coverage job declared `timeout-minutes` before this was written, so
both inherited GitHub's six-hour default. The three inner tiers were correctly
ordered, which is what made the gap easy to miss: nothing was wrong until
something hung outside `cargo`, and then nothing would have stopped it for six
hours.

### The clocks do not start together

Comparing the configured numbers is not enough because the timers start at
different moments and cover different work.

The watchdog starts when `cargo` starts, so it covers the build as well as the
test run, while nextest's global timeout starts only once tests begin. A
watchdog merely larger than the global timeout still pre-empts it whenever the
build takes longer than the difference. Here the difference is 1,200 s, which
is ample for this crate's build.

Hitting the global timeout does not stop the run instantly either. nextest
signals the process group and waits `slow-timeout.grace-period`, five seconds
here, before killing it; on Windows termination is immediate and the grace
period is ignored for timeouts. That allowance is seconds rather than minutes,
but it is not zero, and the contract reads it from the configuration so a
profile that raised it raises the requirement too.

The job timer starts when the job starts, before the checkout and the toolchain
setup, and it is still running through the plain `cargo nextest` step and the
Loom models that follow coverage. So a ceiling merely above the watchdog still
cancels the job before the watchdog can report an overrun, and a cancellation
discards the log that would have explained it.

### What the ceiling is sized against

The watchdog plus the work outside its window, measured from the worst of many
runs rather than one. Runs of every conclusion are read, not only successful
ones: a run cancelled at its ceiling is the very case the sizing exists to
prevent, so excluding it would size the ceiling against the runs that never
needed it.

| Lane                                  | Worst coverage step | Worst whole job | Widest gap | Run         |
| ------------------------------------- | ------------------- | --------------- | ---------- | ----------- |
| `ci.yml` `build-test`                 | 343 s               | 1,312 s         | 969 s      | 30024924292 |
| `coverage-main.yml` `coverage-upload` | 303 s               | 353 s           | 42 s       | 29354687551 |

*Table: measured coverage-step and whole-job durations. The gap is the job's
duration less its coverage steps, so it is the work the job timer bounds and
the watchdog does not.*

The sample is the last 115 `ci.yml` coverage jobs, 44 successful, 55 failed and
16 cancelled, and all 19 runs of `coverage-main.yml`, all successful. The worst
cancelled job reached 727 s of its 3,600 s budget, so no run in the sample was
ended by any of these four timers.

The widest gap is 969 s, so the contract allows 20 minutes, making the
requirement 50 minutes, and the ceilings are 65: fifteen above it, as the
estate asks, rather than the ten that 60 gave. That is a rise from the 15
minutes first written here, which the wider sample showed to be below the worst
gap already observed. On the pull-request lane most of that gap is the suite's
own `cargo nextest` step and the Loom models, which run outside the coverage
step and so outside the watchdog.

None of those runs was genuinely cold. One run is the coldest seen so far, not
a measurement of the cold case.

### The contract

`scripts/tests/test_timeout_ordering_contract.py`, run by `make test-scripts`,
asserts the ordering by value over every job invoking the coverage action, in
both the `.yml` and `.yaml` extensions. It reads the watchdog from the step,
then the job, then the workflow, as GitHub resolves it, and it fails on a
coverage-invoking job that declares no ceiling at all. The readings it rests on
live in `scripts/tests/timeout_budgets.py`, `nextest_budgets.py` and
`coverage_lanes.py`, and are exercised on their own in
`test_timeout_reading_contract.py`.

The nextest configuration is parsed as TOML rather than matched as text. A text
match finds a `slow-timeout` inside a comment, inside a `filter` string, or in
a table nextest never consults. The commented-out `global-timeout` is the case
that matters most: a scraping reader would go on reporting a tier that had been
switched off, and the four-tier contract would pass with three.

`terminate-after` is optional, and a `slow-timeout` without it marks a test
slow and never stops it, so the reading refuses that form rather than reporting
one period as the budget. Every table in `.config/nextest.toml` sets it
explicitly.

Durations are read with the grammar `humantime` accepts, which is what nextest
deserializes them with: one or more whole-number components each carrying a
unit, written `180s`, `1m 30s` or `1m30s`, with the long unit spellings and with
no fractional values. A reader taking a single short-unit component would reject
`1m 30s`, `1day` and `1w`, which nextest loads, and the contract would then fail
on a correct file and name the file rather than the reader. Case is significant,
`m` being minutes and `M` months. A duration nextest would refuse raises
`NextestConfigurationError`, the error the rest of these readings report faults
with, rather than tripping an assertion that `python -O` would strip.

The contract also pins the condition each lane carries. A skipped step runs no
`cargo`, so its watchdog never arms and the tiers say nothing about it:
`if: false` on the step or on its job would leave a lane that looks bounded and
is not. The conditions are pinned rather than forbidden, because the one here
is legitimate: `ci.yml` runs the coverage step on the unprivileged leg of a
matrix that also runs as root, and only that leg measures coverage.
`coverage-main.yml` runs on the trunk and carries no condition. A lane gaining,
losing or changing a condition has to change this section with it, and a lane
appearing without an entry fails the contract too.

It pins two values as well as ordering them: the 10 m `global-timeout` and the
65 m job ceiling. Two numbers are involved and they are worth keeping apart.

The **base requirement is 50 minutes**: the 1,800 s watchdog plus the 1,200 s of
measured work outside its window. That is what the job has to be allowed to
take.

The **configured ceiling is 65 minutes**: the base requirement plus a 900 s
margin. The margin is a term of what the contract demands rather than slack
above it, because a ceiling equal to the base requirement cancels the job at the
moment the watchdog would have reported the overrun, and the report is the only
thing that makes an overrun actionable. The ceiling was 60 minutes, which left
only ten. The ordering holds for a wide range of both values, so on its own it
would let either drift away from the table above without failing anything. It
also requires the `global-timeout` to be present rather than skipping when it is
absent, since a skipped test would let this tier be deleted and leave a
four-tier contract passing with three.

The termination allowance it demands between the whole-run budget and the
watchdog is two terms, not one: the largest `grace-period` the configuration
sets, five seconds here, plus a fixed 60-second safety margin. A grace period
is what nextest promises a test after `SIGTERM`; the margin covers the process
teardown and report writing that follow it. Folding them into a single floor
would make raising the grace period from five seconds to thirty look free,
since both would vanish below the margin.

Two readings it makes explicit because both are easy to get wrong and neither
is exercised by this repository's own values:

- The per-test budget is `period` multiplied by `terminate-after`. Every
  multiplier here is one, so a reading that ignored it entirely would give the
  same answer against this file. The assertion is therefore driven with
  controlled configurations rather than this one.
- `period` and `grace-period` sit in the same inline table, so a matcher
  reading the first as a substring would take a grace period for a per-test
  budget whenever it were the larger.

`cross-platform-tests` and `binstall-packaging` also declare no ceiling. They
invoke no coverage step, so they are outside this contract, and bounding them
is separate work.

[shared-actions-coverage]: https://github.com/leynos/shared-actions/blob/main/.github/actions/generate-coverage/README.md

## Further reading

- `tests/e2e_postgresql_embedded_diesel.rs` – example of combining the helper
  with Diesel-based integration tests while running under `root`.
