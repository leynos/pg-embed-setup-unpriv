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

## Coverage publication

Pull requests measure coverage and compare it with the ratchet baseline on the
runner; they never contact CodeScene. `coverage-main.yml` is the only
publisher: it uploads on a push to `main`, serialized by the concurrency group
`${{ github.workflow }}-${{ github.ref }}`, which never cancels a run in
progress.

The token is kept out of every `env` mapping, because the upload action is a
composite whose nested steps inherit its environment. A step with an id, no
`if:` and no `env` runs one command,
`echo "available=${{ secrets.CS_ACCESS_TOKEN != '' }}" >> "$GITHUB_OUTPUT"`.
The upload step runs only when that output is `'true'` and the ref is
`refs/heads/main`, takes the token directly from the secret through its
`access-token` input, and suppresses no failure with `continue-on-error`.

Two gaps are known. A merge made by the Dependabot automerge workflow uses the
workflow token, and GitHub starts no workflow for a push made with that token.
So this workflow does not run for those merges, and the baseline waits for the
next push ([shared-actions issue 518][shared-actions-518] tracks a dispatch).
Only one run waits in a concurrency group, and a newer run replaces a pending
one. A dispatch that replaces a pending push publishes its own commit's
coverage to CodeScene when the token is available, but `generate-coverage`
saves the ratchet baseline only on a push, so the baseline stays one commit
behind until the next push.

Those orderings hold for triggered runs, a push or a dispatch. A manual "Re-run
jobs" on an older `main` run is an operator action outside them: it keeps that
run's commit, so it republishes that commit's coverage and ratchet baseline,
and they stand until the next push supersedes them.

[shared-actions-518]: https://github.com/leynos/shared-actions/issues/518

`make test-scripts` holds the workflows to that shape through these modules
under `scripts/tests/`:

- `workflow_reader.py` parses workflows as GitHub reads them. It refuses a
  mapping key declared twice, reads every trigger spelling (scalar, sequence,
  or mapping, under the bare `on` key that YAML 1.1 reads as `True` or the
  quoted one), and follows calls to local reusable workflows, so a workflow
  that only answers `workflow_call` is judged as a pull-request lane when one
  calls it. A local call carrying an `@` ref is refused.
- `step_conditions.py` reads `if:` conditions: it splits them on `&&`,
  refuses a disjunction, and accepts a step as able to run only when every
  conjunct is one it can show holds. Matrix comparisons must hold together in
  one leg, expanded as GitHub expands `include` and `exclude`. Other conjuncts
  must be a running status or hold in the run being asked about: a pull-request
  event, or the trunk push for the publisher. Anything it does not recognize
  reads as "may never run".
- `publisher_token.py` holds the token rules above: the one check step, the
  upload's condition and input, and no `env` or other step naming the token.
- `coverage_shape_rules.py` states each rule as a function returning its
  offenders. A pull-request coverage step counts only when its conditions let
  it run, and the publisher must generate coverage before it uploads.
  `test_coverage_shape_contract.py` applies them to this repository's workflows.
- `test_coverage_shape_probes.py`, `test_coverage_shape_contacts.py`,
  `test_coverage_shape_publisher.py`, `test_coverage_shape_runnability.py` and
  `test_workflow_reader.py` construct the hazard each rule or reading exists
  for and assert it is named, so a rule that could never fire does not pass
  unnoticed. `test_coverage_shape_properties.py` holds the readers to their
  invariants over generated input.

The reader and the rules are scoped to these contract tests. A new workflow
contract should reuse `workflow_reader.py` rather than parse workflows with
`yaml.safe_load`, which keeps the last of two duplicate keys and says nothing.

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

Run `make spelling` to enforce en-GB-oxendict spelling. The target invokes the
pinned shared `typos-config-builder` gate, which regenerates `typos.toml`,
scans the tracked Markdown with the pinned Typos release, and enforces the
shared phrase corrections that Typos cannot express.

The tracked `typos.toml` is regenerated on every run from the live shared
dictionary and the narrow repository policy in `typos.local.toml`; never edit
the generated file by hand. The gate refreshes the untracked shared-dictionary
cache only when its authority is newer, and a valid cache remains usable when
the network is unavailable. Because the dictionary is live, `typos.toml` must
never be drift checked in continuous integration.

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

## Path resolution

`bootstrap()` resolves the installation and data directories once, in
`src/bootstrap/prepare/mod.rs`, before anything is created on disk:

1. `PG_RUNTIME_DIR` and `PG_DATA_DIR` win outright for their leaf.
2. Otherwise the leaf is `<root>/install` or `<root>/data`, where the root is
   `PG_EMBED_ROOT` when set.
3. Otherwise, on Linux and the BSDs, the root is `/var/tmp/pg-embed-{uid}`
   (`default_root_for`); the uid is `nobody`'s when running as root and the
   current user's otherwise. macOS and Windows have no per-user root and keep
   the `postgresql_embedded` defaults.

`default_paths_under(root)` is the single place that derives the two leaves, so
the privileged and portable resolvers cannot drift. Each resolver emits an
info-level `settings_decision` event with `root_source` (`Override`,
`PerUserDefault` or `SettingsDefault`), both directories, whether each was
derived, and the effective `max_connections`, so a bootstrap log shows which
override won. That last field is read from the resolved settings, not from
`PgEnvCfg`: a test bootstrap with no `PG_MAX_CONNECTIONS` still runs at 20
because `apply_worker_limits` put it there, and the event reports 20 rather
than nothing. A plain bootstrap that sets no limit leaves the key absent and
the event reads `server default`. `PG_MAX_CONNECTIONS` itself is validated in
`PgEnvCfg::to_settings` (floor `MIN_MAX_CONNECTIONS`, currently 4) before the
paths are resolved.

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

| Tier | What it bounds | Where it is set | Current value |
| --- | --- | --- | --- |
| Per-test `slow-timeout` | one test | `.config/nextest.toml` | 180 s default; 30 s and 360 s for two overrides |
| nextest `global-timeout` | the whole test run | `.config/nextest.toml` | 600 s (10 m) |
| Cargo watchdog | one `cargo` invocation, wall clock | `RUN_RUST_CARGO_WAIT_TIMEOUT` at job level | 1,800 s (30 m) |
| Job `timeout-minutes` | the whole job | job level in `ci.yml` and `coverage-main.yml` | 60 m |

*Table: the timers that can end a run, innermost first.*

### The outermost tier was missing

Neither coverage job declared `timeout-minutes` before this was written, so
both inherited GitHub's six-hour default. The three inner tiers were correctly
ordered, which is what made the gap easy to miss: nothing was wrong until
something hung outside `cargo`, and then nothing would have stopped it for six
hours.

### The clocks do not start together

Comparing the configured numbers is not enough, because the timers start at
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

The watchdog plus the work outside its window, measured from the worst of
several runs rather than one:

| Lane | Worst coverage step | Worst whole job | Outside the step | Run |
| --- | --- | --- | --- | --- |
| `ci.yml` `build-test` | 327 s | 1,111 s | 830 s | 33982759833 |
| `coverage-main.yml` `coverage-upload` | 320 s | 353 s | 38 s | 29784541706 |

*Table: measured coverage-step and whole-job durations, read across ten
successful runs of each workflow.*

The widest gap is 830 s, so the contract allows 15 minutes, making the
requirement 45 minutes against ceilings of 60. On the pull-request lane most of
that gap is the suite's own `cargo nextest` step and the Loom models, which run
outside the coverage step and so outside the watchdog.

None of those runs was genuinely cold. One run is the coldest seen so far, not
a measurement of the cold case.

### The contract

`scripts/tests/test_timeout_ordering_contract.py`, run by `make test-scripts`,
asserts the ordering by value over every job invoking the coverage action, in
both the `.yml` and `.yaml` extensions. It reads a step's own environment
before the job's, as GitHub resolves it, and it fails on a coverage-invoking
job that declares no ceiling at all.

Two readings it makes explicit, because both are easy to get wrong and neither
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

## Password reuse

`src/bootstrap/prepare/password.rs` keeps a reused data directory reachable.
`stored_cluster_password` is the query: `Ok(None)` when the data directory has
no `PG_VERSION` marker, the stored password otherwise, and an error (with the
`PG_PASSWORD` and remove-the-cluster remedies) when the file the bootstrap
handed to `initdb` is missing, unreadable, or empty. `reuse_existing_password`
is the command: it applies the query to `settings.password` unless the caller
supplied an explicit password, and returns a `PasswordReuseOutcome`. That enum
covers the three success results only. It supplies the bounded `outcome` field
of the `password_reuse` tracing event on those branches; the four failure
labels below are values of the same field that the enum does not carry, because
a failure returns an error rather than an outcome. Neither a path nor a secret
is ever a label. Both bootstrap paths (`bootstrap_unprivileged` and
`bootstrap_with_root`) call it immediately after the settings paths are
resolved and before the sanitized settings are logged, so the password file is
the one that `resolve_settings_paths_*` derived (`<install>/.pgpass`). Both
functions and the outcome enum are exported at the crate root.

The query publishes nothing. `stored_cluster_password`, and the two private
helpers behind it, read and categorize but never emit, so calling the public
query has no observable effect beyond its return value. A failure carries its
bounded label to the caller in a private `PasswordQueryFailure`, and
`reuse_existing_password` is the single place that emits: the `password_reuse`
event at warning level with `outcome` set to `probe_failed`, `missing_file`,
`unreadable_file`, or `empty_file`, before the error is returned. Those four
are additional bounded values of the tracing field, beyond the three the
returned enum carries, so the field's full label set has seven values and
matches `PasswordReuseOutcomeMetric` rather than `PasswordReuseOutcome`. A
bootstrap that refuses a stale cluster is therefore visible in the log without
the caller rendering the error, while the query stays free of side effects.

### Metrics

`src/observability.rs` holds the metric seam alongside the log target. There is
no metrics dependency: a library should not pick one for its consumer, so the
crate defines `Metric`, a `MetricsRecorder` trait, and
`install_metrics_recorder`, which returns a guard restoring the previous
recorder on drop. That mirrors the crate's other process-wide hooks. With no
recorder installed, `observability::record` is a read lock, a branch and a
return.

`Metric::PasswordReuse` carries a `PasswordReuseOutcomeMetric`, an enum rather
than a string. That is the point of the design: the label set is bounded by
construction, so no password or path can reach a metric, and the requirement is
a property of the type rather than something a reviewer has to police.
`ProbeFailed` and `UnreadableFile` are separate variants even though both map to
`ClusterPasswordUnreadable`, because the label would otherwise collapse two
different operational failures.

`reuse_existing_password` records exactly one count per call, on every branch,
and the query records none. The tests in `password_tests.rs` under
`mod metrics` pin all seven outcomes, verify that the query is silent, and
assert that neither the password nor either directory path appears in a
recorded metric. They carry `#[serial(metrics_recorder)]`, because the recorder
is process-wide and two tests installing concurrently would collect each
other's counts.

### The end-to-end test

`tests/password_reuse_e2e.rs` is the only test that proves the adopted password
authenticates: it starts a real cluster with no `PG_PASSWORD`, bootstraps a
second time against the same directories, and opens a connection with whatever
that second bootstrap chose. Three things about it are deliberate.

- It is gated on `cfg(all(unix, feature = "diesel-support"))`. Opening the
  connection needs diesel and therefore `libpq`, and the macOS and Windows
  lanes build `--no-default-features --features cluster-unit-tests,async-api`
  on runners with no `libpq`. The Linux lane runs `--all-features` and does
  execute it.
- It joins the `serial` group in `.config/nextest.toml`, alongside every other
  binary that starts a cluster. It takes the process lock and starts a
  postmaster, so running it concurrently starves both itself and the tests
  sharing that lock.
- Its crate name is in the `no_std_fs_operations` exclusion list in
  `dylint.toml`, for the reason the list already gives for the other
  integration-test crates: they share the ambient `tests/support` modules,
  which stage fixtures through `std::fs`. A new test crate that touches those
  modules fails the lint until it is added, which is the point; the policy
  decision stays visible.
