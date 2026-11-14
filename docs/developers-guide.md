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
- Every binary that takes that lock directly, and every binary's embedded
  `serial::tests::*` lock cases, joins the one-slot nextest `serial` group.
  nextest does not count a wait for a group slot against `slow-timeout`, but it
  does count time blocked in `flock` once the test has started.
  `tests/support/serial.rs` reports a completed wait of one second or more on
  stderr, so a timeout separates lock queueing from cluster work. A test that
  nextest terminates while it is still blocked emits no such line: the
  diagnostic covers waits that completed, not waits in progress.
  `tests/scenario_lock_wait.rs` drives that path end to end: it holds the lock,
  a spawned child blocks in the same `acquire_process_lock` call, and the
  parent releases only after the child has waited past the threshold, then
  asserts the captured stderr names the lock file and the completed duration.
  The binary gives the child a private `CARGO_TARGET_DIR`, so it takes no
  `serial` group slot.
- Tests that use the local guard with an isolated root stay out of the group.
  `observability`, `data_dir_recovery` and `test_cluster_async` are the
  examples. They share no lock or directory with the other scenarios, so
  serializing them would add wall time without preventing contention. Their
  embedded `serial::tests::*` cases still join through the test-name term.

## Fetching dependencies before tests

Several tests start their own `cargo`, trybuild fixtures among them. Cargo
resolves dependencies while it holds the package-cache lock in `$CARGO_HOME`,
so a nested `cargo` that resolves cold can wait on another repository's build
for many minutes. The fix is to fetch once, before any test starts:

- `make test` depends on the `fetch` target, which runs
  `cargo fetch --locked`. A failed fetch stops the run.
- Every `ci.yml` job that runs tests, and `coverage-main.yml`, runs
  `cargo fetch --locked` as an unconditional step before its first test or
  coverage step, with no `|| true`.

Prepare a direct or offline run with `cargo fetch --locked` first. The estate
lint rule `nested-cargo-offline-tests` checks both.

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
runs-on: >-
  ${{ (github.event.pull_request.head.repo.fork
  || github.event.pull_request.user.login == 'dependabot[bot]')
  && 'ubuntu-latest' || 'ubicloud-standard-2' }}
```

`build-test` is `standard-4` on a measured shortfall: its unprivileged leg ran
out of disk on `standard-2` (`No space left on device`, run 36565376334). A
pull request from a fork cannot obtain an Ubicloud runner, so it falls back to
`ubuntu-latest`. So does a pull request Dependabot opened: hosted minutes cost
nothing on a public repository, and Dependabot opens many pull requests. The
expression tests the pull request's author, not `github.actor`, because the
actor is whoever triggered the run, so a person pushing to a Dependabot branch
would flip the runner. A push and a dispatch have no pull request, so both
values are empty strings (falsy) and the expression selects Ubicloud; a
Dependabot pull request therefore builds cold on the hosted pool.

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
the expression for a push or dispatch, a same-repository pull request, a fork
and a Dependabot pull request, rejects a literal label, inverted arms, another
label, another condition, a missing Dependabot arm and an actor test, and
asserts an exact inventory of the jobs that can land on Ubicloud with their
ceilings. A change that adds, removes or re-times such a job fails it until the
inventory is updated in the same commit.

## The sccache wrapper in CI

The pinned `setup-rust` action (`e693d81d`) starts an sccache server and exports
`RUSTC_WRAPPER` (and `SCCACHE_CONF`) for the whole job. The following CI steps
use an empty `RUSTC_WRAPPER`, which Cargo treats as unset:

- **Root lane** (`Install cargo-nextest and test (root)`): `run_as_root` passes
  `RUSTC_WRAPPER=` through `sudo -E env`. Root's rustc would otherwise talk to
  a server owned by the runner user, which cannot write into the target
  directory root created, so the first crate fails with "Permission denied".
- **Unprivileged test step** (`Test with all features (unprivileged)`): nested
  trybuild compilations have exceeded the slow timeout when using sccache on
  hosted runners. The non-Windows UI cases now share one trybuild invocation,
  avoiding competing Cargo processes for the same generated build directory.
- **Windows test and packaging jobs** use plain rustc because sccache's expanded
  command exceeds the Windows process command-line limit.

`scripts/tests/test_rustc_wrapper_contract.py` checks the root and unprivileged
steps. Local builds retain the configured compiler admission wrapper, including
nested trybuild compilations.

## Test worker staging

The `test_support::worker_binary_for_tests` helper resolves the worker once per
process, preferring `PG_EMBEDDED_WORKER`, then Cargo's
`CARGO_BIN_EXE_pg_worker`, then local discovery. The result, including no
worker found, is cached for that process. On Unix, the selected executable is
copied into a profile- and source-path-specific directory under the system
temporary directory so privilege-dropped test processes can run it. If staging
fails, the helper retains the original path; it does not report the staging
error. Publishing the optional `target/{profile}/pg_worker_staged.path` cleanup
pointer is best-effort, and pointer-write errors are ignored.

The staged executable is replaced by copying to a temporary sibling, setting
mode `0o755`, then atomically renaming it into place. This lets another test
process continue using the previous inode during replacement. Restaging uses a
metadata heuristic: it occurs when the source modification time is newer, the
file sizes differ, or the staged mode is not `0o755`. It does not compare file
contents, so same-size changes with an unchanged or older source timestamp may
not be detected. The pointer file is also written to a temporary sibling and
renamed into place. This staging and discovery contract belongs to test
support; it does not describe production worker installation or updates.

The source precedence, per-process cache, fallback, metadata checks, and atomic
executable and pointer publication are covered by
`src/test_support/worker_env_tests.rs` and
`src/test_support/worker_env_additional_tests.rs`.

## Lint and formatting toolchain

The repository pins a dated nightly in `rust-toolchain.toml` because the
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
4. `$(WHITAKER) --lib whitaker_suite -- --all-targets --all-features`, the
   Whitaker Dylint suite, with `DYLINT_RUSTFLAGS="-D warnings"` denying lint
   warnings (the driver's own `RUSTFLAGS` are cleared first). The route selects
   the aggregate suite rather than `--all` because the rolling release also
   stages a standalone `string_continuation_style` library that the suite
   already registers; loading both aborts rustc with a duplicate lint
   (`leynos/whitaker#565`), and the pinned installer cannot prune the extra
   library. Key lints include `module_max_lines` (a 400-line cap per module) and
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
the installer version named by `WHITAKER_INSTALLER_VERSION`, currently
`0.2.12`, which builds the Dylint driver from a verified Clippy snapshot
(earlier versions clone Clippy's deleted `master` branch and fail on a cold
build). The action is pinned to a full commit SHA of shared-actions, which the
Dependabot shared-actions group bumps.

Which revisions satisfy the concordat QG-002 rule is the rule's decision: its
`compliant_install_whitaker_refs` list is derived from reviewed roots, not
handwritten. `tests/whitaker_install_pin.rs` therefore checks the shape of the
workflow's pin (a full lowercase SHA on the shared action, one `uses` entry)
rather than a value or a copy of the rule's list. A value assertion passes for
exactly one upstream commit and turns red on the next approved bump, and a
copied list has to be refreshed by hand each time. Parameterized negative cases
prove a truncated SHA, a branch name and an uppercase SHA are all refused.

The previous inline step fell back to
`cargo install --locked whitaker-installer`, building the tool from source in
CI and verifying nothing; the suite still refuses every Cargo route to the
installer. Note that the installer still resolves the lint suite from the tip
of the Whitaker repository, so the lints themselves are not yet pinned; a suite
change can turn this gate red without any commit here. That is what happened
between 2026-08-19 and 2026-09-04, when the same installer version and
toolchain built suite commit `b4d3101` instead of `2bc0c3f` and the gate failed
on every branch. Two issues track closing the gap:
[whitaker#402][whitaker-suite-pin] asks the installer for a ref or
suite-version input, and [shared-actions#454][shared-actions-suite-pin] asks
`install-whitaker` to expose and pass it through.

[whitaker-suite-pin]: https://github.com/leynos/whitaker/issues/402
[shared-actions-suite-pin]: https://github.com/leynos/shared-actions/issues/454

### Markdown lint toolchain

`make markdownlint` lints the Markdown with `markdownlint-cli2` after enforcing
en-GB-oxendict spelling. File selection, rule configuration and ignores live
only in `.markdownlint-cli2.jsonc`. The CI action step and the Makefile pass no
positional globs, because the CLI appends configuration globs to command-line
globs, so a caller glob would widen the set rather than replace it.

The local CLI runs through `npx` and is pinned by `MARKDOWNLINT_CLI2_VERSION`
in the `Makefile` (currently `0.23.2`), which must equal the version bundled by
the pinned `DavidAnson/markdownlint-cli2-action` commit in `ci.yml`. Node.js 22
or later is required. Override `MDLINT` only to run the same pinned version
through a different runner. `scripts/tests/test_markdown_gate_contract.py`
checks the pairing, the glob-free invocations, the shared selection and
membership of `make all`.

When the action commit changes, update its entry in `APPROVED_PAIRINGS` in
`scripts/tests/markdown_gate.py` and `MARKDOWNLINT_CLI2_VERSION` in the
`Makefile` together; the contract test fails until both agree.

### Markdown gate helpers

The gate's checks are split into two pure judgement modules and one behavioural
test, so each judgement can be driven against a mutated fixture before it is
applied to the real files.

`scripts/tests/markdown_gate.py` judges the workflow and the configuration.
Every function takes text or a parsed `Workflow` and returns a list of fault
messages: an empty list means the input satisfies the contract, and each
message names the offending value.

- `config_faults(text)` takes the JSONC configuration document. It reports
  malformed JSONC, a top level that is not a JSON object, a `globs` value that
  is not a non-empty array, a selection that omits `**/*.md`, a negation that
  cancels the Markdown selection, and an `ignores` entry that removes every
  Markdown file. It never raises a parser exception.
- `glob_faults(workflow)` takes the parsed workflow and reports any `globs`
  input passed to the action step; the step must pass none.
- `pairing_faults(revision, version)` takes an action commit SHA and a CLI
  version and reports a revision absent from `APPROVED_PAIRINGS` or a version
  that disagrees with its approved entry.
- `pairing_faults_for(workflow, makefile)` applies that pairing to every
  action step, using the version `makefile_version(makefile)` reads from
  `MARKDOWNLINT_CLI2_VERSION`.
- `makefile_version(makefile)` returns the pinned version, or an empty
  string when the assignment is absent.

`scripts/tests/makefile_gate.py` judges the Makefile. Its functions take the
Makefile text and a target name and return fault messages in the same way.

- `prerequisites(makefile, target)` returns the names on the target's
  declaration line; `prerequisite_faults(makefile, target, wanted)` reports
  each wanted name that is missing.
- `recipe_lines(makefile, target)` returns the target's recipe with leading
  tabs removed.
- `mdlint_faults(makefile, target)` reports a recipe that does not invoke
  `$(MDLINT)`, mentions it without running it, ignores its failure with Make's
  `-` prefix, or passes any argument other than `--fix`. `fmt` must pass
  `--fix`; every other target must pass nothing.
- `mdlint_runner_faults(makefile)` requires the default `MDLINT` to be
  exactly `npx --yes markdownlint-cli2@$(MARKDOWNLINT_CLI2_VERSION)`, so a
  no-op that merely mentions the pinned version is reported.

`scripts/tests/test_markdown_gate_e2e.py` runs the gate itself in a temporary
tree holding a copy of the repository configuration. It checks that clean
documents pass, a document under an ignored directory is never linted, and an
invalid document fails and names itself. It also runs `make markdownlint` with
the repository's own `MDLINT` definition, declared prerequisites and recipe,
and the bare pinned CLI that the CI action runs, then compares the set of
flagged files. Every selectable document in that tree carries a violation, so
the flagged set is the selected set and a path cannot drop a document or lint
an ignored one without changing it. The spelling prerequisite is declared but
stubbed, because its gate needs the repository's git history and network
toolchain. The tests need Node.js, `make` and a reachable package registry;
they skip on a development host but fail in CI when either tool is unavailable.

## Spelling policy

Run `make spelling` to enforce en-GB-oxendict spelling. The target invokes the
pinned shared `typos-config-builder` gate, which regenerates `typos.toml`,
scans the tracked Markdown with the pinned Typos release, and enforces the
shared phrase corrections that Typos cannot express.

`TYPOS_CONFIG_BUILDER_VERSION` in the `Makefile` pins the
`typos-config-builder` release the gate runs (currently `v0.1.3`). Raise it
together with the regenerated `typos.toml`, never on its own.

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
Local Make does not provision Nixie, Merman or that toolchain, so `make all`
omits `nixie`; run it by hand after changing a Mermaid diagram.

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

The same target holds the nextest timeout contracts. They share
`scripts/tests/nextest_filters.py`, a reader that accepts only a flat
disjunction of `binary(=name)` terms and, for the serial group, one exact
`kind(test) & test(/^serial::/)` term selecting the lock's embedded unit tests;
`all()`, an intersection, negation, nesting and a duplicated embedded term are
refused rather than interpreted. The reader carries doctests, which the second
`make test-scripts` command collects with `--doctest-modules`.

`test_nextest_tier_contract.py` pins the default, cluster, UI and whole-run
budgets and the exact cluster and serial registries by value.
`test_nextest_filters_contract.py` pins the reader's accepted and refused
shapes, and `test_timeout_ordering_contract.py` and
`test_timeout_exactness_contract.py` check the tiers against each other and the
watchdog. `test_cluster_tier_coverage_contract.py` derives the child-spawning
cluster binaries from the test sources, so a binary missing from both manually
maintained lists is still caught. Timeout-tier membership is separate from
serial-group membership: a binary whose cluster roots are isolated pays cold
startup without taking the shared scenario lock.
`test_nextest_filters_properties.py` uses Hypothesis for filters no file in the
repository writes, such as a name using every permitted character and a term
placed anywhere in the disjunction.

## Minimum supported Rust version

`rust-version` in `Cargo.toml` is a promise to downstream projects. It is 1.94
because `postgresql_embedded` 0.22.0 and `postgresql_archive` 0.22.0, the floor
this crate requires, need 1.94. The committed `Cargo.lock` resolves to the
newest releases, so building from it proves nothing about that promise: 0.6.1
declared 1.85 while a direct `postgresql_archive` requirement resolved to a
release that needs 1.94, and a project on 1.93 could not build it.

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

## File-locking dependency

The binary cache's `CacheLock` uses `fs4` 1.1.0 for cross-platform file
locks[^1]. It is licensed `MIT OR Apache-2.0` and was released on 28 April
2026. Its declared minimum Rust version is 1.75, comfortably below this crate's
1.92 floor. The dependency is declared with `default-features = false` and only
the `sync` feature, because the cache is synchronous and the async backends
would add unused dependencies. Upstream `al8n/fs4` is actively maintained;
1.1.0 was its latest release when this note was written.

[^1]: `fs4` on crates.io: <https://crates.io/crates/fs4>

## The default PostgreSQL version

`PgEnvCfg::apply_version` in `src/lib.rs` sets `Settings::version` from the
configured `version_req`, supplied by `PG_VERSION_REQ` or another configuration
source. If none supplies a requirement, the settings keep their default,
`VersionReq::STAR`, so upstream's newest listed release wins.

Development builds between 0.6.3 and 0.7.0 held that back with a default of
`=18.6.0`, which never shipped in a release. The `TimeZone` failure reached
downstream projects because the release that started shipping a bundled
`share/timezone` tree (18.6.1) also packed most of it as tar hard links, and
`postgresql_archive` before 0.22.0 dropped hard links while extracting, so
`share/timezone/UTC` was missing and `SET TimeZone = 'UTC'` failed with
`invalid value for parameter "TimeZone": "UTC"`. `postgresql_archive` 0.22.0
extracts hard links, which is why the MSRV is 1.94 and the pin is gone.
`tests/timezone_regression.rs` starts an unpinned cluster and asks the server
for named zones, so a regression in extraction fails there and not in a
downstream project.

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

| Tier                     | What it bounds                     | Where it is set                               | Current value                                            |
| ------------------------ | ---------------------------------- | --------------------------------------------- | -------------------------------------------------------- |
| Per-test `slow-timeout`  | one test                           | `.config/nextest.toml`                        | 180 s default; 30 s, 300 s and 900 s for three overrides |
| nextest `global-timeout` | the whole test run                 | `.config/nextest.toml`                        | 1,100 s                                                  |
| Cargo watchdog           | one `cargo` invocation, wall clock | `RUN_RUST_CARGO_WAIT_TIMEOUT` at job level    | 1,800 s (30 m)                                           |
| Job `timeout-minutes`    | the whole job                      | job level in `ci.yml` and `coverage-main.yml` | 66 m                                                     |

*Table: the timers that can end a run, innermost first.*

### The cluster tier is sized against a measurement

The 180 s default was nextest's own default, not a figure chosen for these
tests. On the 6-core Rocky 10 host where #247 was filed, a single
`test_cluster_fixture unit_tests::fixture_exposes_connection_metadata` run with
nothing else in the same nextest run took 132.893 s. That leaves about 26% of
headroom under 180 s, and one full `make test` on the same host timed out sixty
cluster tests at exactly 180.00 s.

The cluster override gives the twenty binaries that start or share a real
PostgreSQL cluster 300 s. That is 2.25 times the measurement, leaving 167 s of
headroom for a loaded developer host or a cold CI runner. The five-second grace
period is explicit: omitting it would select nextest's ten-second default and
raise the watchdog floor the contract derives from 1,765 s to 1,770 s.

The tier also covers parent tests whose children bootstrap a real cluster:
`shutdown_hook_lifecycle` was already listed, and the three added for #247 are
`durability_defaults`, `fresh_cluster_per_process` and
`per_cluster_directories`. The three added here have isolated roots, so they
take no scenario lock and stay out of the `serial` group, but the parent's
allowance still covers child startup and waiting. Tier membership and
serial-group membership are separate questions, and the contracts check each
list on its own terms.

The cluster tier is 300 s, not the 360 s the `ui` override used before it was
raised for the macOS trybuild builds. `ui` remains at 900 s.

A repeat on a warm host of the same class, after the tier was in place, ran the
same single test in 1.479 s and the whole suite in 920 s: 1,062 of 1,063 tests
passed with no cluster timeout, and the slowest cluster test took 1.900 s. The
132.893 s figure is the cold, uncontended measurement the tier is sized
against; the warm repeat shows the allowance stops binding once the install
tree is cached. The run's one timeout was the `ui` trybuild test, which
compiles six fixtures inside a shared and heavily loaded compiler pool and has
its own 900 s tier.

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
build takes longer than the difference. Here the difference is 700 s, which
covers the 600 s cold-build allowance and 65 s termination allowance enforced
by the contract.

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

It pins two values as well as ordering them: the 1,100 s `global-timeout` and
the 66 m job ceiling. Two numbers are involved and they are worth keeping apart.

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
  binary that takes the process lock directly. It takes that lock and starts a
  postmaster, so running it concurrently starves both itself and the tests
  sharing the lock. The group makes the wait for the lock nextest queue time,
  which `slow-timeout` does not count, instead of runtime `flock` waiting,
  which it does. The wait itself is unchanged: `flock` already serialized these
  tests.
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

`build.rs` also registers the `privileged_unix_platform` cfg
(`cargo:rustc-check-cfg`) and emits it when `CARGO_CFG_TARGET_OS` is `linux`,
`android`, `freebsd`, `openbsd` or `dragonfly`. It reruns when
`CARGO_CFG_TARGET_OS` or `TARGET` changes. The classification lives in
`build_support/target_os.rs`, which both the build script and
`tests/build_target_os.rs` include, so the accepted values, an unsupported
value and an absent value are tested directly. The `cfg_aliases` build
dependency is gone.

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
  Each case creates its own temporary root, so the cases share no state, take
  no cross-process scenario lock, and stay out of the `serial` group. They run
  in parallel with one another and with the grouped scenarios, and serializing
  them would only lengthen the run.

`cluster::connection::admin_connect_error` wraps a failed admin connection. It
keeps the `postgres` error as the source and appends that error's own source to
the message, because `tokio_postgres` displays a server-side failure as
`db error` alone.

## Disposable test clusters

`src/durability.rs` makes test clusters non-durable (#297).
`PgEnvCfg::to_settings_with_context(true)`, the path `to_settings_for_tests` and
`bootstrap_for_tests` take, adds `fsync`, `synchronous_commit` and
`full_page_writes` set to `off` to `Settings::configuration`, unless
`PG_EMBED_DURABLE=1`, in which case it states `fsync = on` (upstream starts
every server with `-F`, so only an explicit `-c fsync=on` after it restores
durability). The plain `to_settings`, used by `run` and the binary, adds
nothing.

The `fsync = off` entry is also the marker that a cluster is disposable.
`initialize_without_sync` (exported from the hidden `worker` module for the
worker binary) reads it: for a disposable cluster whose binaries are installed
and whose data directory has no `postgresql.conf`, it runs `initdb --no-sync`
itself, mirroring upstream's arguments, before `PostgreSQL::setup`, which then
skips initialization. Both in-process starts call it through
`durability::setup_disposable`, and `pg_worker`'s setup operation calls it
directly, so the root path gets it too through the settings snapshot. The
`initdb` it runs comes from the installation `PostgreSQL::setup` would choose
(a trusted directory, the directory itself when it is named for a version
`Settings::version` matches, else the highest matching child), so the two never
disagree on the major version; `initdb.exe` is looked up on Windows. It is
bounded by `Settings::timeout`, which the test bootstrap sets to `None` in
favour of `setup_timeout`, and `setup_disposable` ends it when the future
awaiting it is dropped, because a blocking task cannot be aborted: the child is
killed and reaped either way. Like upstream's own `initdb` call it passes
`--encoding=UTF8` and no locale, so `PG_LOCALE` and `PG_ENCODING` keep reaching
the server through `Settings::configuration` only, as before. When the binaries
are not installed yet (a cold install tree), `setup` installs and runs `initdb`
in one step that cannot be interleaved, so that first cluster keeps the synced
`initdb`.

Tests: `src/durability_tests.rs` covers the defaults, the opt-out, the entry
points and `initdb --no-sync` through a stand-in `initdb`, including version
selection, the timeout and cancellation; `tests/durability_defaults.rs` starts
real clusters in child processes and reads `current_setting` back for the
default and the opt-out. Each is mutation-proved.

## Per-cluster data directories

ADR 005 records the design; this section covers the code.

- `src/bootstrap/prepare/cluster_slot/mod.rs` holds `claim_slot` and
  `sweep_dead_slots`; `cluster_slot/removal.rs` removes a swept slot's files.
  `claim_derived_slot` in `prepare/layout.rs` calls `claim_slot` on both
  preparation paths, and only when the data directory was derived from a root.
  It takes `.claim-guard` first and holds it across the sweep and the creation
  of the next slot, so another bootstrap never sees a created but not yet
  locked file and mistakes it for a dead slot. It then sweeps, creates and locks
  `<name>.lock` with `create_new`, and keeps the `File` in a process-global
  list so the lock lasts until exit. Do not drop those files early: a dropped
  lock lets another process sweep a live cluster.
- `src/bootstrap/prepare/orphan.rs` decides whether a dead slot's directory
  still has a server. `OrphanStop` is the seam the tests use to refuse a stop.
  `SignalStop` sends `SIGQUIT` and waits. The identity check binds a PID to the
  slot: the process must be named `postgres` and serve the slot's directory. On
  Linux it reads `/proc/<pid>/comm` and compares `/proc/<pid>/cwd` with the
  directory; an unreadable working directory leaves the slot unconfirmed. On
  other Unix platforms it runs `ps` for the name and `lsof -d cwd` for the
  directory, and a missing or failing tool is unconfirmed. On Windows a live
  PID keeps the directory.
- `src/bootstrap/prepare/orphan_watch.rs` (Linux only; its process and
  registry halves are `orphan_watch_process.rs` and `orphan_watch_registry.rs`,
  and the lifecycle hooks are `src/cluster/startup_watch.rs`) stops a server
  the moment its owner dies (#287). `watch_slot_owner`, called before the
  lifecycle starts the server on the synchronous and asynchronous paths (and
  undone by `release_watcher` if the start fails), spawns
  `setsid flock --exclusive <slot.lock> sh -c WATCHER_SCRIPT`. The flock blocks
  until the kernel releases the owner's slot lock, held for the life of the
  process, and the script then sends `SIGQUIT` only to the PID in
  `postmaster.pid` if `/proc/<pid>/comm` is `postgres` and `/proc/<pid>/cwd` is
  the data directory: the same identity check as the sweep, in shell, so a
  library-only binary needs no helper. `slot_lock_path` distinguishes "not a
  slot" from "cannot tell". Handles are kept per data directory, and
  `cleanup_in_process` and `cleanup_worker_managed_with_runtime` call
  `release_watcher`, because the slot lock outlives the cluster and the watcher
  would otherwise wait until process exit. A missing `setsid` or `flock`, an
  unreadable slot, or `PG_EMBED_ORPHAN_WATCHER=off` skips the watcher, each
  recorded as a bounded `outcome`, both as a debug event and as a
  `Metric::OrphanWatcher` count. While a watcher runs it holds the slot lock,
  so a sweep skips the slot while the watcher holds the slot lock, and the next
  bootstrap reclaims it. The sweep tests set the opt-out so they exercise the
  sweep alone. A non-interactive bash ignores `SIGQUIT`, so the tests use a
  copy of `sleep` named `postgres`.
- `DataLayout` (in `src/bootstrap/prepare/layout.rs`) says whether a derived
  data directory becomes a slot: `PerCluster` for test bootstraps, `Persistent`
  for the setup-only `run`. A `Persistent` bootstrap also clears
  `Settings::temporary` in both preparation paths, so the handle that ran
  `initdb` does not delete the cluster when it drops: `run` leaves it for the
  process that comes next, and the root path needs the same because each worker
  subprocess drops after its operation. `has_live_peers` and `ClaimGuard` (in
  `cluster_slot/mod.rs`) let `plan_cleanup` in `src/cluster/cleanup.rs` demote
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
- Tests: `cluster_slot/tests.rs`, `cluster_slot/claim_guard_tests.rs` and
  `orphan_tests.rs` hold the unit cases. Their `FakePostgres` copies `/bin/sh`
  as `postgres` and runs `read line`. `tests/per_cluster_directories.rs` drives
  real children through concurrent bootstraps, a live-kept and dead-swept sweep
  with an orphaned server, the explicit `PG_DATA_DIR` case, the setup-lock wait
  (Linux only: it reads the blocked waiter from `/proc/locks`), and a run
  killed mid-test. Each case uses a fixed root under `CARGO_TARGET_TMPDIR`, so
  the next run's sweep reclaims whatever a killed run left. A `Running` guard
  closes each child's stdin, waits, then kills it.

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
assigns `RUSTFLAGS` without the fast flags because a measurement should not
depend on them, and the release recipe and workflow keep the platform linker
because they assign `RUSTFLAGS` (even an empty value displaces the
configuration). Cargo has no per-profile `rustflags`, so a direct
`cargo build --release` takes the configuration's flags unless `RUSTFLAGS` is
assigned too.

On Linux, install `mold` before building: the configuration names it, so a
build without it fails at link time. Its flag reaches the linker through the
default `cc` driver, which must be GCC 12.1 or newer, or clang (older GCC does
not accept `-fuse-ld=mold`); installing `clang` does not make it the driver, so
on an older host set `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=clang`. CI
installs mold through `setup-rust`'s `install-mold` input.
`tests/build_standard_contract.rs` holds the standard. It reads the
configuration sources, the commands `make -n` prints for each development
target on a Linux host and a macOS host (each keeping the caller's own
`RUSTFLAGS`) and for the release target (the coverage exclusion is checked in
the workflow steps) on a Linux host, and the `setup-rust` steps of the CI
workflows (each must pass `install-mold`), so a flag lost through a recipe or
workflow edit fails there. The decision is recorded in
[ADR 006](adrs/adr-0006-rust-build-standard.md). The contract runs `make -n`,
so a direct `cargo test` needs GNU make on the `PATH`. It fails when `make` is
missing instead of skipping, so a missing tool cannot read as a pass.

The release recipes assign `RUSTFLAGS` in the Makefile, and the release archive
script assigns it itself when it runs Cargo (the caller's value, or an empty
one), so the configuration's flags never reach a shipped binary whichever entry
point reached it: `make release`, `make release-archive` or the release
workflow's `Build release archive` step. The script tests read that assignment
from the execution context, and the release workflow contract requires every
release build in `release.yml` to go through the script.

### The Whitaker route

`make lint` runs `lint-clippy` (which takes the standard's flags, as before) and
`lint-whitaker`. Whitaker builds with its own installer-managed toolchain,
which must not inherit the repository's development flags, so the route clears
encoded flags, backend, target and linker settings through
`WHITAKER_CLEAN_ENV`, assigns `RUSTFLAGS` empty (an unset variable would let
Cargo fall back to the flags in `.cargo/config.toml`, and the Concordat rule
reads an explicit empty assignment as the isolation boundary), and passes the
warning policy through `DYLINT_RUSTFLAGS`. It selects no LLVM backend, and it
loads only the aggregate `whitaker_suite` library: the rolling release stages a
standalone `string_continuation_style` library beside the suite that registers
the same lint, and `--all` would load both (see `leynos/whitaker#565`).
`tests/build_standard_contract.rs` reads `make -n lint-whitaker` on a Linux and
a macOS host for exactly that. In CI, the root job's direct `cargo build`
assigns the standard's flags in its step environment, because `setup-rust`
would otherwise export `-D warnings` alone.

### The development footprint

The Concordat `rust-dev-build-footprint` rule keeps default development builds
small. `Cargo.toml` sets `incremental = false` and `debug = 0` in
`[profile.dev]`, which the test profile inherits, and `.cargo/config.toml` sets
`[unstable] embed-metadata = false`, the native Cargo setting for dated
nightlies from 2026-07-31. The trade-off is that a debugger sees no symbols or
line tables in a default development build and every rebuild recompiles whole
crates. To debug, run with `CARGO_PROFILE_DEV_DEBUG=true`; to rebuild
incrementally, set `CARGO_INCREMENTAL=1`. `tests/build_footprint_contract.rs`
holds all three settings.

### Cranelift

Exception: Cranelift is not the development-profile backend. The estate adopts
it only where the full suite passes under it, and that is not shown here. The
repository pins the nightly in `rust-toolchain.toml`. A partial re-measurement
on `nightly-2026-11-06` (2026-11-07) ran the `test_cluster_fixture` binary with
`--all-features` under Cranelift: 17 of 18 tests passed and
`scenario_fixture_missing_timezone` panicked, while the same binary passed 18
of 18 under LLVM. The full suite was not re-run on it. The earlier full
measurement was on `nightly-2026-04-25` (2026-10-02): 13 tests (the
`scenario_fixture_*` scenarios in `tests/test_cluster_fixture/steps.rs` among
them) failed under it while the suite passed under LLVM, 799 of 799 in the full
run. That is evidence of a past result, not of current behaviour, and no
deferral date is set.

Since that measurement, the tests that used `catch_unwind` only to observe a
failure no longer do: the `test_cluster_fixture` scenarios and unit tests call
a fallible `try_test_cluster` helper (`tests/test_cluster_fixture/world.rs`)
and read the failure message as data, and the "should not panic" probes in
`src/env/tests` and `src/cluster/startup_tests.rs` are plain calls. The tests
that still need unwinding are genuine: `lifecycle_template`'s panic cleanup,
`ScopedEnv`'s poisoned-lock recovery and its post-panic state checks in
`src/env/tests`, the Loom unwind model in `src/env/loom_tests`, and the
non-Unix lock tests in `tests/support/serial/non_unix_tests.rs`, which do not
build on Linux. The `resume_unwind` helpers (`src/env/tests/thread_helpers.rs`,
`src/cluster/shutdown_hook/tests.rs`) are not tests of unwinding: they
propagate a worker thread's panic to the test. The `test_cluster_fixture`
binary now passes 18 of 18 under Cranelift on `nightly-2026-11-06` (2026-11-14;
it was 17 of 18 before), but the rest of the suite has not been re-measured
since the conversion. Re-measure the whole suite under the backend, with CI's
environment, on the next attempt to adopt it, and adopt it only if every test
passes.
