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

The publisher job runs in the `codescene` environment, whose deployment policy
admits `main` alone. That job is the only one to declare it, and no workflow a
pull request can start declares it in any job, since a declaration there would
let branch code ask for the token. The environment is repository configuration
and is not changed from this repository's files.

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

`make test-workflow-contracts` holds the workflows to that shape by running
`cv005-contracts check`, the shared contract library in `leynos/shared-actions`
(`packages/cv005-contracts`), from a full commit named by `CV005_CONTRACTS_REF`
in the Makefile; CI runs it in a "Check the CV-005 contracts" step. A fix to
the rules is therefore a pin bump. The target needs `uv`, which fetches the
Python 3.13 the library runs under. The repository's parameters are in
`.github/cv005.toml`: its `repository` name and one `[[pairing]]` for the
pull-request coverage step, which runs on the unprivileged leg of the
`build-test` matrix alone, so its condition is the pull-request guard plus
`matrix.privilege == 'unprivileged'`. The library's own suite proves each rule
refuses the shape it exists to refuse, so this repository keeps no copy of the
readers or the refusal cases. It reads workflows strictly, refusing a mapping
key declared twice and reading every trigger spelling, and it follows calls to
local reusable workflows, so a workflow that only answers `workflow_call` is
judged as a pull-request lane when one calls it. It also holds the uploader's
retired inputs retired: no `installer-checksum` input and no
`CODESCENE_CLI_SHA256` variable. The generator and uploader actions must share
one commit.

`make test-scripts` holds the rest of the workflow contracts through
`scripts/tests/workflow_reader.py`, which parses workflows as GitHub reads
them: it refuses a mapping key declared twice and reads every trigger spelling
(scalar, sequence, or mapping, under the bare `on` key that YAML 1.1 reads as
`True` or the quoted one). `test_workflow_reader.py` constructs the hazards it
exists for. A new workflow contract should reuse `workflow_reader.py` rather
than parse workflows with `yaml.safe_load`, which keeps the last of two
duplicate keys and says nothing.

## Runner placement

`ci.yml`'s `build-test` runs on `ubicloud-standard-4` and `coverage-main.yml`'s
`coverage-upload`, main's only cache writer, on `ubicloud-standard-2`.
`runs-on` selects the class with the runner-selection expression, shown here for
`standard-2`:

```yaml
runs-on: ${{ github.event.pull_request.head.repo.fork && 'ubuntu-latest' || 'ubicloud-standard-2' }}
```

`build-test` is `standard-4` on a measured shortfall: its unprivileged leg ran
out of disk on `standard-2` (`No space left on device`, run 36565376334). A
pull request from a fork cannot obtain an Ubicloud runner, so it falls back to
`ubuntu-latest`; a push and a dispatch have no pull request, so the fork value
is null and they select Ubicloud.

The writer sits on Ubicloud because Ubicloud's cache proxy is scoped by ref. A
pull request's Ubicloud lane reads a warm main scope only when a main job on
Ubicloud writes it, so a lane can move to Ubicloud only after its main writer
has.

An Ubicloud runner is a self-hosted just-in-time runner, so GitHub's six-hour
cap for hosted jobs does not bound it and a hung job would hold a billable
runner. Every job whose `runs-on` can select Ubicloud therefore states its own
`timeout-minutes`. Both keep their 66 minutes, which the timeout ordering
contract holds above the 1,800 s cargo watchdog, so neither can be set to twice
a warm run. A fork's pull request restores a hosted cache that main no longer
refreshes; fork pull requests are rare here, and a second hosted writer would
pay double on every main push.

`binstall-packaging` places only its Linux row on Ubicloud. Its `runs-on` reads
`${{ matrix.runner }}`, and the Linux row's `runner` value is the
runner-selection expression itself, with the pinned `ubuntu-24.04` as the fork
arm; the macOS and Windows rows are unchanged, and the required check names
(which carry the target, not the runner) are too. The job's one ceiling covers
its slowest row: 15 minutes, twice the 5.5 the Windows row took (run
36721523547), rounded up to 5. The contract reads a matrix-placed job through
its rows: each row that names Ubicloud is judged as a runner-selection
expression, the hosted rows are left alone, and Ubicloud named under a key the
`runs-on` does not read is refused.

`scripts/tests/test_runner_placement.py` holds this to the files. It evaluates
the expression for a push or dispatch, a same-repository pull request and a
fork, rejects a literal label, inverted arms, another label and another
condition, and asserts an exact inventory of the jobs that can land on Ubicloud
with their ceilings. A change that adds, removes or re-times such a job fails
it until the inventory is updated in the same commit.

## The sccache wrapper in CI

From the `setup-rust` pin `ff1dd759`, the action starts an sccache server and
exports `RUSTC_WRAPPER` (and `SCCACHE_CONF`) for the whole job. Two steps of
`build-test` must not use it, so each sets an empty `RUSTC_WRAPPER`, which
cargo treats as unset:

- **Root lane** (`Install cargo-nextest and test (root)`): `run_as_root` passes
  `RUSTC_WRAPPER=` through `sudo -E env`. Root's rustc would otherwise talk to
  a server owned by the runner user, which cannot write into the target
  directory root created, so the first crate fails with "Permission denied".
- **Unprivileged test step** (`Test with all features (unprivileged)`): the `ui`
  tests build trybuild fixtures in nested cargo invocations that inherit the
  wrapper, and under it exceeded their 360 s slow-timeout where they took about
  125 s without. The cause of the slowdown is not traced.

`scripts/tests/test_rustc_wrapper_contract.py` holds both steps to this: each
way of letting the wrapper back in is driven through the judge in
`scripts/tests/rustc_wrapper.py` first, and then `ci.yml` is judged. Every
other step keeps sccache.

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
the installer version named by `WHITAKER_INSTALLER_VERSION`, currently `0.2.9`,
the floor the action accepts (it refuses anything lower). The action is pinned
to shared-actions `6cec89bac47a21cf756d68d638a9a510998e57f8` (#546), which also
carries the 60 s sccache startup fix. The concordat QG-002 rule accepts it
because the action directory is content-identical to the reviewed `6dea5677`
(#522); any later shared-actions commit that leaves it unchanged is also
accepted. The previous inline step fell back to
`cargo install --locked whitaker-installer`, building the tool from source in
CI and verifying nothing. `tests/whitaker_install_pin.rs` keeps that
arrangement in place. Note that the installer still resolves the lint suite
from the tip of the Whitaker repository, so the lints themselves are not yet
pinned; a suite change can turn this gate red without any commit here. That is
what happened between 2026-08-19 and 2026-09-04, when the same installer
version and toolchain built suite commit `b4d3101` instead of `2bc0c3f` and the
gate failed on every branch. Two issues track closing the gap:
[whitaker#402][whitaker-suite-pin] asks the installer for a ref or
suite-version input, and [shared-actions#454][shared-actions-suite-pin] asks
`install-whitaker` to expose and pass it through.

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

## Minimum supported Rust version

`rust-version` in `Cargo.toml` is a promise to downstream projects. It is 1.92
because `postgresql_embedded` 0.20.2, the floor this crate requires, needs
1.92. The committed `Cargo.lock` resolves to the newest releases, so building
from it proves nothing about that promise: 0.6.1 declared 1.85 while a direct
`postgresql_archive` requirement resolved to a release that needs 1.94, and a
project on 1.93 could not build it.

`make msrv` closes that gap, and CI runs it as the `msrv` job. It runs
`scripts/msrv_check.py`, which installs the declared toolchain, resolves a
lockfile with Cargo's rust-version-aware resolver
(`CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS=fallback`), and builds every target
with `cargo +<version> check --locked --all-targets --all-features`. It clears
`RUSTFLAGS`, because the repository's Cargo configuration sets nightly-only
flags. The committed `Cargo.lock` is restored afterwards.

Raising a dependency requirement past what the declared version can build now
fails that job. Fix it by relaxing the requirement to the lowest release that
does what is needed, or raise `rust-version` deliberately and say so in the
change log. `scripts/tests/test_msrv_check.py` holds the script's steps and the
contract that CI runs `make msrv`.

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

## Windows scenario lock

On targets without `flock`, `tests/support/serial/non_unix.rs` serializes
behavioural scenarios with a lock directory,
`<target dir>/pg-embed-setup-unpriv.serial.lockdir`, holding an `owner` file.
Taking it is a retry loop with a 120-second deadline: `create_dir` succeeding
means the caller owns the lock, and a failure is classified by
`is_lock_contention`.

- `AlreadyExists` is contention on every platform: another process holds the
  lock, or a stale owner is swept after its two-second grace.
- `PermissionDenied` is contention on Windows only. Windows reports it, instead
  of `AlreadyExists`, while a peer's removal of the directory is still pending
  because a handle on it stays open (#279). Elsewhere it is a real failure and
  panics at once.
- Any other error panics at once, naming the path.

When the deadline passes the panic names the last error, so a path that really
cannot be created is reported with its cause. The tests drive
`try_acquire_with`, which takes the directory creation as a parameter, through
each error kind without racing a real peer.

## Test timeouts: four tiers, outermost last

Four independent timers can end a test run, and the canonical statement of how
they must be ordered lives in the `generate-coverage` README in
[`leynos/shared-actions`][shared-actions-coverage]. All four are set here.

| Tier                     | What it bounds                     | Where it is set                               | Current value                                   |
| ------------------------ | ---------------------------------- | --------------------------------------------- | ----------------------------------------------- |
| Per-test `slow-timeout`  | one test                           | `.config/nextest.toml`                        | 180 s default; 30 s and 450 s for two overrides |
| nextest `global-timeout` | the whole test run                 | `.config/nextest.toml`                        | 600 s (10 m)                                    |
| Cargo watchdog           | one `cargo` invocation, wall clock | `RUN_RUST_CARGO_WAIT_TIMEOUT` at job level    | 1,800 s (30 m)                                  |
| Job `timeout-minutes`    | the whole job                      | job level in `ci.yml` and `coverage-main.yml` | 66 m                                            |

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

The sample is the last 115 `ci.yml` coverage jobs, 44 successful, 55 failed,
and 16 cancelled, and all 19 runs of `coverage-main.yml`, all successful. The
worst cancelled job reached 727 s of its 3,600 s budget, so no run in the
sample was ended by any of these four timers.

The widest gap is 969 s, so the contract allows 20 minutes, making the
requirement 50 minutes. Fifteen minutes above it is the margin the estate asks
for, and the estate's comparison is strict rather than inclusive, so the
ceiling is the next whole minute above that sum: 66 rather than 65, and rather
than the ten minutes of margin that 60 gave. That is a rise from the 15 minutes
first written here, which the wider sample showed to be below the worst gap
already observed. On the pull-request lane most of that gap is the suite's own
`cargo nextest` step and the Loom models, which run outside the coverage step
and so outside the watchdog.

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
deserializes them with. A duration is a sequence of values each carrying a
unit, written `180s`, `1m 30s` or `1m30s`, with the long unit spellings. The
grammar was read from `humantime` 2.3.0, the version nextest resolves through
`humantime_serde`, rather than assumed. A value may carry a fractional part,
and whitespace is skipped wherever a digit could go: `1 0s` is ten seconds, and
`1.5m` and `1 . 5 m` are both ninety. A leading point, a trailing point and a
second point are refused. `0` is the one duration written without a unit, and
only in that exact form: `parse_duration` compares the untrimmed string, so
` 0 ` misses the shortcut and fails as a number with no unit.

The arithmetic uses integers, and splits into seconds and nanoseconds the way
`humantime` splits it because a floating-point reading accepts two classes of
text the runner refuses. `humantime` divides a fraction into its unit and
errors on any remainder, so `0.0000000002s` is an error rather than a rounding.
Every intermediate is held in a `u64`, so an oversized value is refused rather
than becoming a large float; the denominator is one of those intermediates, so
twenty fractional digits are refused however small the numerator is. Digits are
ASCII, `'0'..='9'` and nothing else, so an Arabic-Indic digit is not a number
there. The split matters too: whole hours, days, weeks, months, and years are
counted in seconds, so a duration of several centuries stays in range where a
single nanosecond counter would overflow.

Where the two halves meet is the subtle part, and the reading follows
`humantime` step for step rather than summing and carrying once at the end. The
running total is normalized after every whole part and every fraction, so a
nanosecond part that passes the `u64` ceiling at one addition is refused
however short the duration it names, and one that carries cleanly is read:
`18446744073709551615ns` twice over is thirty-seven seconds and is refused,
while the same value plus `1ns` is read. The carry itself is in two parts
because `humantime`'s is. Its own normalization runs only when the nanosecond
part is above one second, so a part of exactly one second reaches the duration
constructor, which carries it and aborts if that carry overflows. That is why
`0.5s 0.5s` is one second while `18446744073709551615s 500ms 500ms` is refused;
a reader made merely stricter to refuse the second gets the first wrong.

The whole set of inputs this was measured against, and the acceptance gate of
zero disagreements, is the estate's humantime reader differential rather than
anything invented here. The reading lives in
`scripts/tests/nextest_durations.py`, and the unit tables and patterns it
applies in `scripts/tests/nextest_duration_grammar.py`. The two are apart
because they are different kinds of statement: one is a transcription of
`humantime`'s tables, the other the checked arithmetic that applies them. The
abbreviations `wk`, `wks`, `yr` and `yrs` and the micro sign in `µs` are
accepted alongside the longer spellings. A reader taking a single short-unit
component would reject `1m 30s`, `1day` and `1w`, which nextest loads, and the
contract would then fail on a correct file and name the file rather than the
reader. Case is significant, `m` being minutes and `M` months. A duration
nextest would refuse raises `NextestConfigurationError`, the error the rest of
these readings report faults with, rather than tripping an assertion that
`python -O` would strip.

Exactness has to survive the comparison as well as the reading, and that is a
separate place to lose it. Every tier comparison is a sum, and each sum mixes a
duration from `.config/nextest.toml` with a budget read from a workflow and
with a constant declared in `timeout_budgets`. A sum is only as exact as its
least exact term: one `float` among them converts the whole of it back, and the
conversion is silent. So the workflow budgets are read as exact values too, and
every constant the tiers add is one. Above two to the fifty-third a `float` no
longer holds every integer second, and two budgets nextest reads as different
compare equal there, so an ordering that must hold strictly would pass on a
configuration that violates it.
`scripts/tests/test_timeout_exactness_contract.py` drives each composition with
inputs one second apart and far larger than anything this repository will
configure, which is exactly why the loss cannot be exposed by the real files: a
contract resting on them would pass with every term a `float`.

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
66 m job ceiling. Two numbers are involved and they are worth keeping apart.

The **base requirement is 50 minutes**: the 1,800 s watchdog plus the 1,200 s
of measured work outside its window. That is what the job has to be allowed to
take.

The **configured ceiling is 66 minutes**: the base requirement plus a 900 s
margin, and then the next whole minute above that sum. The margin is a term of
what the contract demands rather than slack above it, because a ceiling equal
to the base requirement cancels the job at the moment the watchdog would have
reported the overrun, and the report is the only thing that makes an overrun
actionable. The estate states the comparison as strict as well as margined, and
`shared-actions`' own `ceiling_is_sufficient` applies it that way, so 65
minutes would sit exactly on the requirement and fail it. The ceiling was 60
minutes, which left only ten. The ordering holds for a wide range of both
values, so on its own it would let either drift away from the table above
without failing anything. It also requires the `global-timeout` to be present
rather than skipping when it is absent, since a skipped test would let this
tier be deleted and leave a four-tier contract passing with three.

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

`cross-platform-tests` also declares no ceiling. It invokes no coverage step,
so it is outside this contract, and bounding it is separate work.
`binstall-packaging` has a 15-minute job-level ceiling (see "Runner
placement"), which the placement contract holds; it is outside this contract
for the same reason.

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

## Extension hook

The extension hook installs prebuilt, digest-verified extension archives into
the embedded tree between `Setup` and `Start`. Its user-facing contract is in
[`docs/extensions.md`](extensions.md); this section covers the internals.

### Modules

- `src/extensions/mod.rs`: the public surface (`ExtensionRequest`,
  `InstalledExtension`, `install_extensions`, `install_extensions_async`,
  `compile_target()`), the per-run span, and the two-phase orchestration: every
  requested name is selected against the manifest first, then each archive is
  acquired and installed.
- `config.rs`: `PG_EXTENSIONS*` to `ExtensionRequest`, and the extension
  cache directory resolution, which mirrors `cache::resolve_cache_dir`.
- `manifest/`: four modules, one question each. `mod.rs` holds the schema-1
  types, `Manifest::parse` and the validation; `select.rs` chooses one artefact
  for the running server, matching the `PostgreSQL` major **and** minor and the
  compile target, with no cross-minor fallback; `fetch.rs` is `load` (a
  filesystem path, `https://`, or loopback `http://`; size-capped and
  digest-verified); `source.rs` is `ManifestSource` and the redaction its
  `location()` applies. The digest is mandatory for every URL source, loopback
  included, and optional for a path.
- `archive.rs`: per-digest cache under `cache::CacheLock`, downloads to a
  permitted URL with a redirect policy that refuses any target the same rule
  would refuse, bounded retries for connection failures and 5xx, streaming
  SHA-256. Permitted means `https://` anywhere or `http://` to a loopback
  address, which is the one rule `is_permitted_url`, the GET and the redirect
  policy all consult; describing the download as HTTPS-only sends a reader
  looking for a second policy that does not exist. A cache entry is classified
  without following symlinks, so a link whose target hashes correctly is
  cleared rather than reused.
- `install.rs`: the archive is read into memory, re-hashed against the
  manifest digest, validated in full (`classify_entry_path`, the manifest
  `files` list) and only then written, each file to a temporary sibling that is
  renamed over the destination, with `0o755`/`0o644` modes and a chown to the
  tree owner.
- `tree.rs` and `write.rs`: the installation tree as one `cap-std` directory
  handle held until the last rename, and the per-file writer that goes through
  it. Before writing, `inspect_destination` reports what is already there as
  one of five outcomes: an identical regular file, which is reused and only has
  its mode and ownership repaired; an absent, non-regular or differing
  destination; or one that could not be opened, stated or read. A symlink
  arrives as the last of those, because the open carries `O_NOFOLLOW` and so
  refuses it rather than following it to a file outside the tree. A directory
  reaches the non-regular outcome on Unix and the unreadable one on Windows,
  where it cannot be opened for reading without backup semantics; both refuse
  it, and the tests assert the outcome each platform actually produces rather
  than accepting either. Only the identical case changes what the writer does,
  but the outcomes are kept apart rather than collapsed into "not identical":
  an absent or differing destination is the ordinary course of an install,
  while an unreadable or non-regular one is a repair the operator should be
  able to see named, and it is logged at debug with its reason.
- `version.rs`: running-version detection from the versioned directory name,
  then `bin/pg_config --version`.
- `name.rs` and `digest.rs`: validated newtypes and the `HashingWriter`.

### Lifecycle seam

`src/cluster/extension_hook.rs` owns `run_post_setup` (and its async twin).
`startup.rs` passes a `LifecycleContext` (runtime, environment, and a
`PostSetup` carrying the binary-cache configuration and hit flag) through
`run_lifecycle_steps`, which executes `Setup`, the hook, `Start` and the port
refresh with a dispatcher closure for the root and unprivileged cases. The hook
first refreshes the installation directory, then populates the binary cache on
a miss, then installs. Ordering invariants: the binary cache sees the pristine
Theseus tree, never extension files; extension files exist before `Start`; the
CLI setup-only path (`startup_setup_only.rs`) runs the hook without a `Start`.

### Compile target

`build.rs` exports Cargo's `TARGET` as `PG_EMBED_TARGET`; `compile_target()` is
a `const fn` over `env!`, which `tests/ui/pass/extensions_compile_target.rs`
proves usable in a const context. Manifest artefacts match on that triple.

### Tests

Unit tests live under `src/extensions/tests/` with an in-memory archive builder
that writes hostile names straight into GNU headers, a loopback HTTP server
with canned response sequences (retries, redirects, 4xx), proptests over entry
paths and the hashing writer, and end-to-end pipeline tests. The lifecycle
ordering is asserted in `src/cluster/startup_tests.rs` through the
root-operation hook. `tests/extensions_install.rs` is the rstest-bdd suite and
`tests/test_cluster_extensions.rs` loads a probe module (a renamed `autoinc`)
into a real cluster, synchronously and asynchronously.

## Shared-cluster retry

`src/test_support/bootstrap_retry/` holds the bounded retry that the two
shared-cluster singletons in `test_support/shared_singleton.rs` put around
`TestCluster::new_split` and `TestCluster::new`. The design document records
why ("Implementation update: bounded retry of transient bootstrap failures").
This section covers the internals and the rules for changing them.

- **Scope.** `retry_transient` is for the singletons alone, because they cache
  their first failure for the life of the process. Do not wrap other
  constructors in it: a direct `TestCluster::new` caller owns its own policy.
- **Classification is by type.** `is_transient` reads the report's cause chain
  for `postgresql_embedded::Error`, the crate's `LifecycleTimeout`, and the
  `ExtensionArchiveUnavailable` kind. A new transient cause gets a typed error,
  never a message match. `postgresql_archive` is a direct dependency only so
  the archive variants can be named; it must stay at the version
  `postgresql_embedded` uses, which the unit tests enforce by construction.
- **Cache hits.** `cache_integration::note_cached_binaries` wraps a lifecycle
  failure that followed a binary-cache hit in `CachedBinariesUsed`, except for
  extension kinds (`BootstrapErrorKind::is_extension`). The marker turns an I/O
  or archive cause deterministic and tells the user to remove the entry.
- **Tests.** Unit and property tests for the retry live in
  `bootstrap_retry/tests.rs`, and the marker's tests in
  `cluster/cache_integration_tests.rs`. `tests/shared_cluster_retry.rs` drives
  each singleton in a child process, with the fault injected through
  `Command::env`. Keep it that way: the singletons are process-wide, and the
  test rules forbid mutating the environment in process.

## Stale password files

`src/bootstrap/prepare/stale_password.rs` holds
`discard_orphaned_password_file`. Both preparation paths call it:
`bootstrap_unprivileged` in `prepare/mod.rs` and the root path in
`prepare/root.rs`. Each calls it after the data directory is prepared and
before the password file's permissions are set. When the data directory holds no
`PG_VERSION`, it removes the install tree's password file, so that
`postgresql_embedded` writes the password the bootstrap reports before `initdb`
reads it (#259). The design document records the reasoning.

- **Scope.** Call it only from preparation. It reuses `password.rs`'s
  `has_cluster_marker`, so the "no cluster" test is the one password reuse
  applies, including its refusal to read an unsearchable directory as empty.
- **Failure.** A path that cannot be removed returns
  `ClusterPasswordUnreadable` and logs a `warn` event naming the path and the
  I/O error kind, never the password.
- **Tests.** `stale_password_tests.rs` holds the unit cases.
  `tests/bootstrap_cases/pgpass.rs`, a module of the `bootstrap_for_tests`
  binary, drives preparation through `bootstrap_for_tests`, and it is the only
  test that reaches the root path, in the CI root lane.
  `tests/fresh_cluster_per_process.rs` runs two child processes in one root.
  Each case creates its own temporary root, so the cases share no state and
  stay out of the nextest `serial` group.

`cluster::connection::admin_connect_error` wraps a failed admin connection. It
keeps the `postgres` error as the source and appends that error's own source to
the message, because `tokio_postgres` displays a server-side failure as
`db error` alone.

## Per-cluster data directories

ADR 005 records the design; this section covers the code.

- `src/bootstrap/prepare/cluster_slot.rs` holds `claim_slot` and
  `sweep_dead_slots`. `claim_derived_slot` in `prepare/layout.rs` calls
  `claim_slot` on both preparation paths, and only when the data directory was
  derived from a root. It sweeps first, then creates and locks `<name>.lock`
  with `create_new`, and keeps the `File` in a process-global list so the lock
  lasts until exit. Do not drop those files early: a dropped lock lets another
  process sweep a live cluster.
- `src/bootstrap/prepare/orphan.rs` decides whether a dead slot's directory
  still has a server. `OrphanStop` is the seam the tests use to refuse a stop.
  `SignalStop` sends `SIGQUIT` and waits. The identity check binds a PID to the
  slot: the process must be named `postgres` and serve the slot's directory. On
  Linux it reads `/proc/<pid>/comm` and compares `/proc/<pid>/cwd` with the
  directory; an unreadable working directory leaves the slot unconfirmed. On
  other Unix platforms it runs `ps` for the name and `lsof -d cwd` for the
  directory, and a missing or failing tool is unconfirmed. On Windows a live
  PID keeps the directory.
- `DataLayout` (in `src/bootstrap/prepare/layout.rs`) says whether a derived
  data directory becomes a slot: `PerCluster` for test bootstraps, `Persistent`
  for the setup-only `run`. `has_live_peers` and `ClaimGuard` (in
  `cluster_slot.rs`) let `plan_cleanup` in `src/cluster/cleanup.rs` demote
  `CleanupMode::Full` to data-only while another slot in the root holds its
  lock. The plan holds the claim guard across the probe and the removal, and
  `claim_slot` takes the same guard, so a claim cannot slip between them.
- `src/cluster/setup_lock.rs` holds `SetupLock`, taken by `start_postgres`,
  `start_postgres_async` (on the blocking pool) and the setup-only lifecycle. A
  start holds it while the tree is populated and releases it after a cache hit,
  before `Setup`'s `initdb`; `InstallLock` carries that decision to the
  extension hook, which retakes the lock when it writes into the tree. The lock
  and each lifecycle step emit debug events (`waited_ms`, `elapsed_ms`) so a
  slow start can be attributed to a phase. `tests/start_ramp.rs` measures the
  ramp:
  `START_RAMP_ROOT=/dev/shm/ramp cargo test --test start_ramp -- --ignored
  --nocapture`
  starts N processes at once and prints wall, lock wait and phase medians. Use
  a RAM-backed root to keep `initdb`'s fsync out of the numbers.
- Tests: `cluster_slot_tests.rs` and `orphan_tests.rs` hold the unit cases.
  Their `FakePostgres` copies `/bin/sh` as `postgres` and runs `read line`.
  `tests/per_cluster_directories.rs` drives real children through concurrent
  bootstraps, a live-kept and dead-swept sweep with an orphaned server, the
  explicit `PG_DATA_DIR` case, the setup-lock wait (Linux only: it reads the
  blocked waiter from `/proc/locks`), and a run killed mid-test. Each case uses
  a fixed root under `CARGO_TARGET_TMPDIR`, so the next run's sweep reclaims
  whatever a killed run left. A `Running` guard closes each child's stdin,
  waits, then kills it.

## The build standard

Development, test, lint, and typecheck builds use the parallel `rustc` frontend
(`-Zthreads=8`) and, on Linux, the `mold` linker (`-Clink-arg=-fuse-ld=mold`).
These are defaults in `.cargo/config.toml`, which Cargo discovers on its own,
so a bare `cargo build` gets them. `mold` ships for Linux only, so the linker
flag lives in a Linux-only table and macOS and Windows keep their platform
linker. Cargo selects one `rustflags` source rather than merging them, so every
source repeats the same flags apart from the linker.

An assigned `RUSTFLAGS` replaces the configuration's flags, so the Makefile
recipes that set it compose the standard's flags onto any inherited value (CI's
`setup-rust` exports one). Two builds are deliberately excluded: coverage
assigns `RUSTFLAGS` without the fast flags, because a measurement should not
depend on them, and the release recipe and workflow keep the platform linker,
because they assign `RUSTFLAGS` (even an empty value displaces the
configuration). Cargo has no per-profile `rustflags`, so a direct
`cargo build --release` takes the configuration's flags unless `RUSTFLAGS` is
assigned too.

On Linux, install `mold` before building: the configuration names it, so a
build without it fails at link time. CI installs it through `setup-rust`'s
`install-mold` input. `tests/build_standard_contract.rs` holds the standard. It
reads the configuration sources, the commands `make -n` prints for each
development target on a Linux host and a macOS host (each keeping the caller's
own `RUSTFLAGS`) and for each coverage and release target on a Linux host, and
the `setup-rust` steps of the CI workflows (each must pass `install-mold`), so
a flag lost through a recipe or workflow edit fails there.

### Cranelift

Exception: Cranelift is not the development-profile backend. The estate adopts
it only where the full suite passes under it, and that is not shown here on the
pinned `nightly-2026-04-25` (measured 2026-10-02): 13 tests
(`steps::scenario_fix...` among them) fail under it while the suite passes
under LLVM, 799 of 799 in the full run. Revisit on the next toolchain bump:
measure the whole suite under the backend, with CI's environment, and adopt it
if every test passes.
